; ModuleID = 'InputKt'
source_filename = "InputKt.kt"

@.fmt.int_println = private unnamed_addr constant [4 x i8] c"%d\0A\00", align 1

declare i32 @printf(ptr, ...)

define i32 @InputKt_sumTo(i32 %arg0) {
entry:
  %merge_2 = alloca i32
  %merge_5 = alloca i32
  %t0 = add i32 0, 0
  store i32 %t0, ptr %merge_2
  %t1 = add i32 0, 1
  store i32 %t1, ptr %merge_5
  br label %bb1
bb1:
  %t2 = load i32, ptr %merge_5
  %t3 = icmp sle i32 %t2, %arg0
  br i1 %t3, label %bb2, label %bb3
bb2:
  %t4 = load i32, ptr %merge_2
  %t5 = load i32, ptr %merge_5
  %t6 = add i32 %t4, %t5
  store i32 %t6, ptr %merge_2
  %t7 = add i32 0, 1
  %t8 = load i32, ptr %merge_5
  %t9 = add i32 %t8, %t7
  store i32 %t9, ptr %merge_5
  br label %bb1
bb3:
  %t10 = load i32, ptr %merge_2
  ret i32 %t10
}

define i32 @main() {
entry:
  %t0 = add i32 0, 10
  %t1 = call i32 @InputKt_sumTo(i32 %t0)
  call i32 (ptr, ...) @printf(ptr @.fmt.int_println, i32 %t1)
  %t3 = add i32 0, 100
  %t4 = call i32 @InputKt_sumTo(i32 %t3)
  call i32 (ptr, ...) @printf(ptr @.fmt.int_println, i32 %t4)
  ret i32 0
}

