; ModuleID = 'InputKt'
source_filename = "InputKt.kt"

@.fmt.int_println = private unnamed_addr constant [4 x i8] c"%d\0A\00", align 1

declare i32 @printf(ptr, ...)

define i32 @main() {
entry:
  %merge_2 = alloca i32
  %merge_6 = alloca i32
  %merge_11 = alloca void
  %t0 = add i32 0, 1
  %t1 = add i32 0, 4
  store i32 %t0, ptr %merge_2
  br label %bb1
bb1:
  %t2 = load i32, ptr %merge_2
  %t3 = icmp sle i32 %t2, %t1
  br i1 %t3, label %bb2, label %bb4
bb2:
  %t4 = add i32 0, 1
  %t5 = add i32 0, 4
  store i32 %t4, ptr %merge_6
  br label %bb5
bb3:
  %t6 = add i32 0, 1
  %t7 = load i32, ptr %merge_2
  %t8 = add i32 %t7, %t6
  store i32 %t8, ptr %merge_2
  br label %bb1
bb4:
  ret i32 0
bb5:
  %t9 = load i32, ptr %merge_6
  %t10 = icmp sle i32 %t9, %t5
  br i1 %t10, label %bb6, label %bb8
bb6:
  %t11 = load i32, ptr %merge_2
  %t12 = load i32, ptr %merge_6
  %t13 = icmp eq i32 %t11, %t12
  br i1 %t13, label %bb9, label %bb10
bb7:
  %t14 = add i32 0, 1
  %t15 = load i32, ptr %merge_6
  %t16 = add i32 %t15, %t14
  store i32 %t16, ptr %merge_6
  br label %bb5
bb8:
  br label %bb3
bb9:
  %t17 = add i32 0, 1
  call i32 (ptr, ...) @printf(ptr @.fmt.int_println, i32 %t17)
  br label %bb11
bb10:
  %t19 = add i32 0, 0
  call i32 (ptr, ...) @printf(ptr @.fmt.int_println, i32 %t19)
  br label %bb11
bb11:
  br label %bb7
}

