; ModuleID = 'InputKt'
source_filename = "InputKt.kt"

@.str.0 = private unnamed_addr constant [5 x i8] c"fib(\00", align 1
@.str.1 = private unnamed_addr constant [5 x i8] c") = \00", align 1
@.fmt.int_println = private unnamed_addr constant [4 x i8] c"%d\0A\00", align 1
@.fmt.concat.0 = private unnamed_addr constant [14 x i8] c"fib(%d) = %d\0A\00", align 1

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
  br i1 %t6, label %bb5, label %bb6
bb5:
  %t7 = load i32, ptr %merge_7
  %t8 = load i32, ptr %merge_9
  %t9 = add i32 %t7, %t8
  %t10 = load i32, ptr %merge_9
  store i32 %t10, ptr %merge_7
  store i32 %t9, ptr %merge_9
  %t11 = add i32 0, 1
  %t12 = load i32, ptr %merge_12
  %t13 = add i32 %t12, %t11
  store i32 %t13, ptr %merge_12
  br label %bb4
bb6:
  %t14 = load i32, ptr %merge_9
  ret i32 %t14
}

define i32 @main() {
entry:
  %merge_2 = alloca i32
  %t0 = add i32 0, 0
  %t1 = add i32 0, 10
  store i32 %t0, ptr %merge_2
  br label %bb1
bb1:
  %t2 = load i32, ptr %merge_2
  %t3 = icmp sle i32 %t2, %t1
  br i1 %t3, label %bb2, label %bb3
bb2:
  %t4 = load i32, ptr %merge_2
  %t5 = load i32, ptr %merge_2
  %t6 = call i32 @InputKt_fib(i32 %t5)
  call i32 (ptr, ...) @printf(ptr @.fmt.concat.0, i32 %t4, i32 %t6)
  %t8 = add i32 0, 1
  %t9 = load i32, ptr %merge_2
  %t10 = add i32 %t9, %t8
  store i32 %t10, ptr %merge_2
  br label %bb1
bb3:
  ret i32 0
}

