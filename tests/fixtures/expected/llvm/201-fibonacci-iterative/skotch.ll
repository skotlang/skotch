; ModuleID = 'InputKt'
source_filename = "InputKt.kt"

@.fmt.int_println = private unnamed_addr constant [4 x i8] c"%d\0A\00", align 1

declare i32 @printf(ptr, ...)

define i32 @InputKt_fib(i32 %arg0) {
entry:
  %merge_7 = alloca i32
  %merge_9 = alloca i32
  %merge_12 = alloca i32
  %t0 = add i32 0, 1
  %t1 = icmp sle i32 %arg0, %t0
  br i1 %t1, label %bb1, label %bb2
bb1:
  ret i32 %arg0
bb2:
  br label %bb3
bb3:
  %t2 = add i32 0, 0
  store i32 %t2, ptr %merge_7
  %t3 = add i32 0, 1
  store i32 %t3, ptr %merge_9
  %t4 = add i32 0, 2
  store i32 %t4, ptr %merge_12
  br label %bb4
bb4:
  %t5 = load i32, ptr %merge_12
  %t6 = icmp sle i32 %t5, %arg0
  br i1 %t6, label %bb5, label %bb7
bb5:
  %t7 = load i32, ptr %merge_7
  %t8 = load i32, ptr %merge_9
  %t9 = add i32 %t7, %t8
  %t10 = load i32, ptr %merge_9
  store i32 %t10, ptr %merge_7
  store i32 %t9, ptr %merge_9
  br label %bb6
bb6:
  %t11 = add i32 0, 1
  %t12 = load i32, ptr %merge_12
  %t13 = add i32 %t12, %t11
  store i32 %t13, ptr %merge_12
  br label %bb4
bb7:
  %t14 = load i32, ptr %merge_9
  ret i32 %t14
}

define i32 @main() {
entry:
  %merge_2 = alloca i32
  %t0 = add i32 0, 0
  %t1 = add i32 0, 12
  store i32 %t0, ptr %merge_2
  br label %bb1
bb1:
  %t2 = load i32, ptr %merge_2
  %t3 = icmp sle i32 %t2, %t1
  br i1 %t3, label %bb2, label %bb4
bb2:
  %t4 = load i32, ptr %merge_2
  %t5 = call i32 @InputKt_fib(i32 %t4)
  call i32 (ptr, ...) @printf(ptr @.fmt.int_println, i32 %t5)
  br label %bb3
bb3:
  %t7 = add i32 0, 1
  %t8 = load i32, ptr %merge_2
  %t9 = add i32 %t8, %t7
  store i32 %t9, ptr %merge_2
  br label %bb1
bb4:
  ret i32 0
}

